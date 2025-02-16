use super::ChooseKernel;
use crate::{
    shapes::*,
    tensor::cuda::{Cuda, CudaArray},
};
use cudarc::driver::{AsKernelParam, CudaSlice, LaunchAsync, LaunchConfig};
use std::sync::Arc;

const PTX_SRC: &str = include_str!(concat!(env!("OUT_DIR"), "/choose.ptx"));

pub(crate) trait HasCudaKernel<E> {
    const MOD: &'static str;
    const FNS: &'static [&'static str];
}

impl HasCudaKernel<f32> for Cuda {
    const MOD: &'static str = "choose_f32";
    const FNS: &'static [&'static str] = &["choose_fwd_f32", "choose_bwd_f32"];
}

impl HasCudaKernel<f64> for Cuda {
    const MOD: &'static str = "choose_f64";
    const FNS: &'static [&'static str] = &["choose_fwd_f64", "choose_bwd_f64"];
}

impl<E: Dtype + AsKernelParam> ChooseKernel<E> for Cuda
where
    Self: HasCudaKernel<E>,
{
    fn forward<S: Shape>(
        &self,
        cond: &Self::Storage<S, bool>,
        lhs: &Self::Storage<S, E>,
        rhs: &Self::Storage<S, E>,
    ) -> Result<Self::Storage<S, E>, Self::Err> {
        if !self.dev.has_func(Self::MOD, Self::FNS[0]) {
            self.dev.load_ptx(PTX_SRC.into(), Self::MOD, Self::FNS)?;
        }

        let shape = lhs.shape;
        let strides = lhs.shape.strides();
        let numel = shape.num_elements();

        let mut storage = unsafe { self.dev.alloc_async::<E>(numel) }?;

        let dims: CudaSlice<usize> = self.dev.take_async(shape.concrete().into())?;
        let cond_strides: CudaSlice<usize> = self.dev.take_async(cond.strides.into())?;
        let lhs_strides: CudaSlice<usize> = self.dev.take_async(lhs.strides.into())?;
        let rhs_strides: CudaSlice<usize> = self.dev.take_async(rhs.strides.into())?;

        let fwd_fn = self.dev.get_func(Self::MOD, Self::FNS[0]).unwrap();
        let cfg = LaunchConfig::for_num_elems(numel as u32);
        let params = (
            numel,              // const size_t numel,
            S::NUM_DIMS,        // const size_t num_dims,
            &dims,              // const size_t *dims,
            cond.data.as_ref(), // const bool *cond,
            &cond_strides,      // const size_t *cond_strides,
            lhs.data.as_ref(),  // const float *lhs,
            &lhs_strides,       // const size_t *lhs_strides,
            rhs.data.as_ref(),  // const float *rhs,
            &rhs_strides,       // const size_t *rhs_strides,
            &mut storage,       // float *out,
        );
        unsafe { fwd_fn.launch_async(cfg, params) }?;
        Ok(CudaArray {
            data: Arc::new(storage),
            shape,
            strides,
        })
    }

    fn backward<S: Shape>(
        &self,
        cond: &Self::Storage<S, bool>,
        grad_lhs: &mut Self::Storage<S, E>,
        grad_rhs: &mut Self::Storage<S, E>,
        grad_out: &Self::Storage<S, E>,
    ) -> Result<(), Self::Err> {
        let bwd_fn = self.dev.get_func(Self::MOD, Self::FNS[1]).unwrap();
        let numel = cond.shape.num_elements();

        let dims: CudaSlice<usize> = self.dev.take_async(cond.shape.concrete().into())?;
        let lhs_strides: CudaSlice<usize> = self.dev.take_async(grad_lhs.strides.into())?;
        let cond_strides: CudaSlice<usize> = self.dev.take_async(cond.strides.into())?;
        let rhs_strides: CudaSlice<usize> = self.dev.take_async(grad_rhs.strides.into())?;

        let cfg = LaunchConfig::for_num_elems(numel as u32);
        let params = (
            numel,                             // const size_t numel,
            S::NUM_DIMS,                       // const size_t num_dims,
            &dims,                             // const size_t *dims,
            cond.data.as_ref(),                // const bool *cond,
            &cond_strides,                     // const size_t *cond_strides,
            Arc::make_mut(&mut grad_lhs.data), // float *grad_lhs,
            &lhs_strides,                      // const size_t *lhs_strides,
            Arc::make_mut(&mut grad_rhs.data), // float *grad_rhs,
            &rhs_strides,                      // const size_t *rhs_strides,
            grad_out.data.as_ref(),            // const float *grad_out,
        );
        unsafe { bwd_fn.launch_async(cfg, params) }?;
        Ok(())
    }
}
